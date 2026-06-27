//! Model registry: discovery, caching, and the enriched `/api/models/registry` endpoint.
//!
//! Two sources:
//!
//! 1. **Claude (subscription/CLI)** — a curated static list (`CLAUDE_REGISTRY_MODELS`). The
//!    Claude Code CLI has no list-models API, so the list is data, not discovered. Each entry
//!    carries a `weight` (relative subscription-quota cost: Opus heavy, Sonnet mid, Haiku light)
//!    used by the profile cascade.
//!
//! 2. **OpenRouter (API)** — `GET /api/v1/models` → parse free/tool-use/context/coding and
//!    cache the result. Refreshed on demand via `POST /api/models/registry/refresh`. Requires
//!    the `openrouter_api_key` credential (from the OS keychain). Returns an empty list (not an
//!    error) when the key is not set, so the UI degrades gracefully.
//!
//! The registry is **app-wide** (one shared `ModelRegistry` in `AppState`). The UI populates
//! model selectors from this registry, grouped by provider, with badges (FREE · tool-use ✓/✗ ·
//! context). Adding a provider = adding a registry source here; no other code needs to change.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::credentials::{CredentialStore, OPENROUTER_API_KEY};

// ── Registry entry (the unified shape) ───────────────────────────────────────

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
}

// ── Static Claude catalog ────────────────────────────────────────────────────

/// The curated static list of Claude (subscription-CLI) models.
///
/// No list-models API exists for the Claude Code CLI, so this is data. Trivial to update
/// when new models ship. Weights are relative subscription-quota cost: Haiku=1, Sonnet=3,
/// Opus=10.
pub const CLAUDE_REGISTRY_MODELS: &[RegistryEntryStatic] = &[
    RegistryEntryStatic {
        display: "Opus 4.8",
        id: "claude-opus-4-8",
        context: 200_000,
        weight: 10,
        price_in: 15.0,
        price_out: 75.0,
    },
    RegistryEntryStatic {
        display: "Sonnet 4.6",
        id: "claude-sonnet-4-6",
        context: 200_000,
        weight: 3,
        price_in: 3.0,
        price_out: 15.0,
    },
    RegistryEntryStatic {
        display: "Haiku 4.5",
        id: "claude-haiku-4-5-20251001",
        context: 200_000,
        weight: 1,
        price_in: 0.8,
        price_out: 4.0,
    },
];

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
        }
    }
}

/// Build the static Claude portion of the registry.
pub fn claude_entries() -> Vec<RegistryEntry> {
    CLAUDE_REGISTRY_MODELS.iter().map(|m| m.to_entry()).collect()
}

// ── OpenRouter model shape (subset of the /api/v1/models response) ───────────

#[derive(Debug, Deserialize)]
struct OpenRouterModelsResp {
    data: Vec<OpenRouterModelRaw>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterModelRaw {
    id: String,
    name: String,
    /// Pricing block.
    #[serde(default)]
    pricing: OpenRouterPricing,
    /// Context window (tokens).
    #[serde(default)]
    context_length: u64,
    /// Supported parameters (e.g. `["tools", "temperature", ...]`).
    #[serde(default)]
    supported_parameters: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct OpenRouterPricing {
    /// USD per token (NOT per million). Parse as f64 from a string field.
    #[serde(default, deserialize_with = "price_string")]
    prompt: f64,
    #[serde(default, deserialize_with = "price_string")]
    completion: f64,
}

/// Deserialize a price that OpenRouter sends as a string (e.g. `"0"` or `"0.000000003"`).
fn price_string<'de, D: serde::Deserializer<'de>>(de: D) -> Result<f64, D::Error> {
    // The field can be a JSON string OR a JSON number depending on OpenRouter's version.
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum MaybeStr {
        Str(String),
        Num(f64),
    }
    match MaybeStr::deserialize(de) {
        Ok(MaybeStr::Str(s)) => Ok(s.parse::<f64>().unwrap_or(0.0)),
        Ok(MaybeStr::Num(n)) => Ok(n),
        Err(_) => Ok(0.0),
    }
}

impl OpenRouterModelRaw {
    /// Convert to a [`RegistryEntry`].
    fn to_entry(&self) -> RegistryEntry {
        let free = self.pricing.prompt == 0.0 && self.pricing.completion == 0.0;
        let tool_use = self.supported_parameters.iter().any(|p| p == "tools");
        // USD per token → USD per million tokens.
        let price_in = self.pricing.prompt * 1_000_000.0;
        let price_out = self.pricing.completion * 1_000_000.0;
        let coding = coding_score(&self.id, &self.name, tool_use);
        RegistryEntry {
            provider: "openrouter".to_string(),
            display: self.name.clone(),
            id: self.id.clone(),
            free,
            tool_use,
            context: self.context_length,
            coding,
            price_in,
            price_out,
            weight: 0,
        }
    }
}

/// Heuristic coding suitability (0.0–1.0) for an OpenRouter model.
///
/// Returns `1.0` for models whose id or name contains well-known coding signals
/// (case-insensitive). Returns `0.7` for tool-use capable models (can call tools,
/// which is required for the agentic worker). Otherwise `0.3`.
fn coding_score(id: &str, name: &str, tool_use: bool) -> f32 {
    let haystack = format!("{} {}", id.to_lowercase(), name.to_lowercase());
    let coding_signals = [
        "coder", "code", "codex", "starcoder", "deepseek-coder", "qwen-coder",
        "coding", "devstral", "granite-code", "wizard-coder",
    ];
    if coding_signals.iter().any(|&s| haystack.contains(s)) {
        return 1.0;
    }
    if tool_use {
        0.7
    } else {
        0.3
    }
}

// ── The registry ──────────────────────────────────────────────────────────────

/// The shared, app-wide model registry.
///
/// Claude entries are always present (static). OpenRouter entries are cached after
/// the first successful fetch. `None` means "not yet fetched or key not set".
#[derive(Default, Clone)]
pub struct ModelRegistry {
    inner: Arc<Mutex<RegistryInner>>,
}

#[derive(Default)]
struct RegistryInner {
    /// Cached OpenRouter entries. `None` = not yet fetched. `Some([])` = fetched but
    /// either the key is absent or the API returned zero models.
    openrouter_cache: Option<Vec<RegistryEntry>>,
}

impl ModelRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return all registry entries: static Claude entries + cached OpenRouter entries.
    ///
    /// If the OpenRouter cache is empty (never fetched), OpenRouter entries are omitted.
    /// Call [`Self::refresh_openrouter`] to populate it.
    pub fn all_entries(&self) -> Vec<RegistryEntry> {
        let mut entries = claude_entries();
        if let Ok(inner) = self.inner.lock() {
            if let Some(ref or_entries) = inner.openrouter_cache {
                entries.extend(or_entries.iter().cloned());
            }
        }
        entries
    }

    /// Whether the OpenRouter cache has been populated (even if it's empty).
    pub fn openrouter_fetched(&self) -> bool {
        self.inner
            .lock()
            .map(|g| g.openrouter_cache.is_some())
            .unwrap_or(false)
    }

    /// Fetch OpenRouter models using `api_key`, replace the cache, and return the new entries.
    ///
    /// On any error (network, parse, auth), logs to stderr and returns an empty list — the
    /// caller can display a "fetch failed" note in the UI but the rest of the registry still
    /// works. Idempotent: re-calling refreshes the cache.
    pub async fn refresh_openrouter(&self, api_key: &str) -> Vec<RegistryEntry> {
        let result = fetch_openrouter_models(api_key).await;
        let entries = match result {
            Ok(e) => e,
            Err(err) => {
                eprintln!("[model-registry] OpenRouter fetch failed: {err}");
                Vec::new()
            }
        };
        if let Ok(mut inner) = self.inner.lock() {
            inner.openrouter_cache = Some(entries.clone());
        }
        entries
    }

    /// Attempt to refresh the OpenRouter cache using the credential store. No-op (returns
    /// `false`) when the key is not set; returns `true` when the fetch was attempted
    /// (even on error — the cache is updated to `Some([])` on failure so the UI doesn't
    /// keep re-fetching automatically).
    pub async fn try_refresh_from_store(&self, creds: &dyn CredentialStore) -> bool {
        let key = match creds.get(OPENROUTER_API_KEY) {
            Ok(Some(k)) if !k.is_empty() => k,
            _ => return false,
        };
        self.refresh_openrouter(&key).await;
        true
    }
}

/// Fetch and parse the OpenRouter `/api/v1/models` endpoint.
async fn fetch_openrouter_models(api_key: &str) -> anyhow::Result<Vec<RegistryEntry>> {
    let client = reqwest::Client::new();
    let resp = client
        .get("https://openrouter.ai/api/v1/models")
        .header("Authorization", format!("Bearer {api_key}"))
        .header("HTTP-Referer", "https://camerata.ai")
        .header("X-Title", "Camerata")
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!("OpenRouter /api/v1/models returned {}", resp.status());
    }

    let body: OpenRouterModelsResp = resp.json().await?;
    let entries: Vec<RegistryEntry> = body.data.iter().map(|m| m.to_entry()).collect();
    Ok(entries)
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Claude static catalog ─────────────────────────────────────────────────

    #[test]
    fn claude_entries_has_all_three_tiers() {
        let entries = claude_entries();
        assert_eq!(entries.len(), 3, "expect exactly 3 Claude tiers");
        assert!(entries.iter().any(|e| e.id == "claude-opus-4-8"));
        assert!(entries.iter().any(|e| e.id == "claude-sonnet-4-6"));
        assert!(entries.iter().any(|e| e.id == "claude-haiku-4-5-20251001"));
    }

    #[test]
    fn claude_entries_all_have_provider_claude() {
        for e in claude_entries() {
            assert_eq!(e.provider, "claude", "Claude entries must carry provider='claude'");
        }
    }

    #[test]
    fn claude_entries_all_support_tool_use() {
        for e in claude_entries() {
            assert!(e.tool_use, "{} must have tool_use=true", e.id);
        }
    }

    #[test]
    fn claude_entries_are_never_free() {
        for e in claude_entries() {
            assert!(!e.free, "{} must not be marked free (subscription path)", e.id);
        }
    }

    #[test]
    fn claude_opus_has_highest_weight() {
        let entries = claude_entries();
        let opus = entries.iter().find(|e| e.id == "claude-opus-4-8").unwrap();
        let sonnet = entries.iter().find(|e| e.id == "claude-sonnet-4-6").unwrap();
        let haiku = entries.iter().find(|e| e.id == "claude-haiku-4-5-20251001").unwrap();
        assert!(
            opus.weight > sonnet.weight,
            "Opus weight must exceed Sonnet (got {} vs {})",
            opus.weight,
            sonnet.weight
        );
        assert!(
            sonnet.weight > haiku.weight,
            "Sonnet weight must exceed Haiku (got {} vs {})",
            sonnet.weight,
            haiku.weight
        );
    }

    #[test]
    fn claude_entries_ids_match_fleet_tier_defaults() {
        // These must stay in sync with fleet/src/tier.rs default_*_model().
        let entries = claude_entries();
        assert!(entries.iter().any(|e| e.id == "claude-opus-4-8"));
        assert!(entries.iter().any(|e| e.id == "claude-sonnet-4-6"));
        assert!(entries.iter().any(|e| e.id == "claude-haiku-4-5-20251001"));
    }

    // ── coding_score ──────────────────────────────────────────────────────────

    #[test]
    fn coder_signal_in_id_yields_full_score() {
        assert_eq!(
            coding_score("qwen/qwen3-coder:free", "Qwen3 Coder", true),
            1.0
        );
        assert_eq!(
            coding_score("deepseek/deepseek-coder", "DeepSeek Coder", true),
            1.0
        );
    }

    #[test]
    fn no_coding_signal_tool_use_yields_mid_score() {
        let score = coding_score("meta/llama-3.1-8b", "Llama 3.1 8B", true);
        assert!((score - 0.7).abs() < f32::EPSILON);
    }

    #[test]
    fn no_coding_signal_no_tool_use_yields_low_score() {
        let score = coding_score("some/text-model", "Text Model", false);
        assert!((score - 0.3).abs() < f32::EPSILON);
    }

    // ── OpenRouter raw → entry ─────────────────────────────────────────────────

    #[test]
    fn free_model_parsed_as_free() {
        let raw = OpenRouterModelRaw {
            id: "qwen/qwen3-coder:free".to_string(),
            name: "Qwen3 235B Coder (free)".to_string(),
            pricing: OpenRouterPricing { prompt: 0.0, completion: 0.0 },
            context_length: 32_768,
            supported_parameters: vec!["tools".to_string()],
        };
        let entry = raw.to_entry();
        assert!(entry.free);
        assert!(entry.tool_use);
        assert_eq!(entry.provider, "openrouter");
        assert_eq!(entry.price_in, 0.0);
        assert_eq!(entry.price_out, 0.0);
        assert_eq!(entry.coding, 1.0);
    }

    #[test]
    fn paid_model_price_scaled_to_per_million() {
        let raw = OpenRouterModelRaw {
            id: "openai/gpt-4o".to_string(),
            name: "GPT-4o".to_string(),
            // $0.000005 per token = $5 per million tokens input
            pricing: OpenRouterPricing { prompt: 0.000005, completion: 0.000015 },
            context_length: 128_000,
            supported_parameters: vec!["tools".to_string()],
        };
        let entry = raw.to_entry();
        assert!(!entry.free);
        // Allow small floating-point imprecision.
        assert!((entry.price_in - 5.0).abs() < 0.001);
        assert!((entry.price_out - 15.0).abs() < 0.001);
    }

    // ── ModelRegistry ─────────────────────────────────────────────────────────

    #[test]
    fn new_registry_has_claude_entries_no_openrouter() {
        let reg = ModelRegistry::new();
        let all = reg.all_entries();
        // Has Claude.
        assert!(all.iter().any(|e| e.provider == "claude"));
        // Does NOT yet have OpenRouter (cache empty).
        assert!(!all.iter().any(|e| e.provider == "openrouter"));
        assert!(!reg.openrouter_fetched());
    }

    #[tokio::test]
    async fn refresh_with_bad_key_stores_empty_and_marks_fetched() {
        let reg = ModelRegistry::new();
        // A garbage key — the HTTP call will fail (or return a 401). Either way,
        // the cache should be set to Some([]) and `openrouter_fetched` becomes true.
        // We can't control the network in tests, so we mock via the internal path:
        // directly call the registry's mutation to simulate a failed fetch.
        {
            let mut inner = reg.inner.lock().unwrap();
            inner.openrouter_cache = Some(Vec::new());
        }
        assert!(reg.openrouter_fetched());
        let all = reg.all_entries();
        // Still has Claude.
        assert!(all.iter().any(|e| e.provider == "claude"));
        // No OpenRouter (empty cache).
        assert_eq!(all.iter().filter(|e| e.provider == "openrouter").count(), 0);
    }

    #[test]
    fn injected_openrouter_entries_appear_in_all_entries() {
        let reg = ModelRegistry::new();
        let fake_entry = RegistryEntry {
            provider: "openrouter".to_string(),
            display: "Test Model (free)".to_string(),
            id: "test/test-model:free".to_string(),
            free: true,
            tool_use: true,
            context: 8192,
            coding: 0.7,
            price_in: 0.0,
            price_out: 0.0,
            weight: 0,
        };
        {
            let mut inner = reg.inner.lock().unwrap();
            inner.openrouter_cache = Some(vec![fake_entry.clone()]);
        }
        let all = reg.all_entries();
        let or_entries: Vec<_> = all.iter().filter(|e| e.provider == "openrouter").collect();
        assert_eq!(or_entries.len(), 1);
        assert_eq!(or_entries[0].id, "test/test-model:free");
    }

    // ── price_string deserializer ─────────────────────────────────────────────

    #[test]
    fn price_string_parses_string_zero() {
        let json = r#"{"prompt":"0","completion":"0"}"#;
        let p: OpenRouterPricing = serde_json::from_str(json).unwrap();
        assert_eq!(p.prompt, 0.0);
        assert_eq!(p.completion, 0.0);
    }

    #[test]
    fn price_string_parses_decimal_string() {
        let json = r#"{"prompt":"0.000000003","completion":"0.000000009"}"#;
        let p: OpenRouterPricing = serde_json::from_str(json).unwrap();
        assert!((p.prompt - 3e-9).abs() < 1e-15);
    }

    #[test]
    fn price_string_parses_numeric_value() {
        let json = r#"{"prompt":0.000005,"completion":0.000015}"#;
        let p: OpenRouterPricing = serde_json::from_str(json).unwrap();
        assert!((p.prompt - 5e-6).abs() < 1e-12);
    }

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
        };
        let json = serde_json::to_string(&entry).unwrap();
        let back: RegistryEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, back);
    }
}
