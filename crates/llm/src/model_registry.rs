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

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::Deserialize;

use crate::credentials::{CredentialStore, OPENROUTER_API_KEY};
use crate::provider_policy::SafeProviders;

// ── Registry entry (the unified shape) ───────────────────────────────────────
//
// `RegistryEntry`, `RegistryEntryStatic` (+ its impl), `RegistryResp`, and `RefreshResp`
// were relocated to `camerata_api_types::model_registry` (Phase A of the DTO
// extraction) — pure wire shapes with no dependency on `ModelRegistry` / the OpenRouter
// fetch types below. Re-exported so every existing `crate::model_registry::X` call site
// keeps resolving unchanged.
pub use camerata_api_types::model_registry::{
    RefreshResp, RegistryEntry, RegistryEntryStatic, RegistryResp,
};

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
        // Anthropic list price: $5 / $25 per million tokens (input / output).
        price_in: 5.0,
        price_out: 25.0,
    },
    RegistryEntryStatic {
        display: "Sonnet 4.6",
        id: "claude-sonnet-4-6",
        context: 200_000,
        weight: 3,
        // Anthropic list price: $3 / $15 per million tokens (input / output).
        price_in: 3.0,
        price_out: 15.0,
    },
    RegistryEntryStatic {
        display: "Haiku 4.5",
        id: "claude-haiku-4-5-20251001",
        context: 200_000,
        weight: 1,
        // Anthropic list price: $1 / $5 per million tokens (input / output).
        price_in: 1.0,
        price_out: 5.0,
    },
];

/// Heuristic: does this model support prompt caching?
///
/// Returns `true` for:
/// - Any `claude`-provider model (all support caching).
/// - OpenRouter models whose id indicates the deepseek, google/gemini, or anthropic family
///   (these providers offer cache-compatible APIs via OpenRouter).
pub fn caching_heuristic(provider: &str, id: &str) -> bool {
    if provider == "claude" {
        return true;
    }
    // For OpenRouter, check the id prefix for supported families.
    let id_lower = id.to_lowercase();
    id_lower.starts_with("deepseek/")
        || id_lower.starts_with("google/")
        || id_lower.starts_with("anthropic/")
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

/// The `architecture` block returned by OpenRouter's `/api/v1/models` endpoint.
///
/// Only `input_modalities` is consumed today; all other architecture fields are ignored.
/// Defaults to an empty modalities list so models that omit the field still parse correctly.
#[derive(Debug, Default, Deserialize)]
struct OpenRouterArchitecture {
    /// Input modalities supported by this model (e.g. `["text"]`, `["text", "image"]`).
    /// A model is considered vision-capable when this list contains `"image"`.
    #[serde(default)]
    input_modalities: Vec<String>,
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
    /// Architecture metadata; used to derive vision capability from `input_modalities`.
    #[serde(default)]
    architecture: OpenRouterArchitecture,
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
        let caching = caching_heuristic("openrouter", &self.id);
        // Vision: parse from `architecture.input_modalities` — true when "image" is listed.
        let vision = self
            .architecture
            .input_modalities
            .iter()
            .any(|m| m == "image");
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
            caching,
            vision,
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
    /// Cached provider data-policy catalog (`/api/frontend/v1/all-providers`), keyed by
    /// provider slug. `None` = not yet fetched. Shared across all models — fetched once,
    /// lazily, on the first call that needs it.
    provider_policy_cache: Option<HashMap<String, ProviderPolicyRecord>>,
    /// Cached, JOINED per-model provider endpoints (pricing + data policy), keyed by
    /// model id. A model id absent from this map means "never fetched for this model"
    /// — see [`ModelRegistry::safe_providers_for`]'s `SafeProviders::Unknown` case. A
    /// present-but-empty `Vec` means "fetched, this model currently has zero providers
    /// serving it" (distinct from zero *safe* providers, which is a non-empty `Vec`
    /// where every entry has `is_safe() == false`).
    provider_endpoints_cache: HashMap<String, Vec<ProviderEndpointInfo>>,
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

    /// Directly seed the OpenRouter cache with a set of entries — a TEST-ONLY seam.
    ///
    /// `#[doc(hidden)]` (not part of the public surface) so it is invisible to normal
    /// callers, but NOT `#[cfg(test)]` so that integration tests in `tests/` (which compile
    /// against the non-test build of the lib) can inject an openrouter-provider model into
    /// the registry without a live HTTP call. Production code never calls it; the
    /// model-selection e2e regression suite does (`tests/model_selection_e2e.rs`).
    #[doc(hidden)]
    pub fn seed_openrouter_entries(&self, entries: Vec<RegistryEntry>) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.openrouter_cache = Some(entries);
        }
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

    // ── Provider-safety data (endpoints + data-policy join) ────────────────────
    //
    // See the module-level "Per-provider data policy" section above for the two
    // OpenRouter endpoints involved and how they're joined. This is the plumbing
    // `provider_policy::provider_constraint_for_request` (the enforcement seam) is built
    // on: it never does I/O itself, it only reads `safe_providers_for`'s answer.

    /// The safe-provider slugs for `model_id` — providers whose live data policy is
    /// `training == false && retains_prompts == false`.
    ///
    /// SYNC, cache-only: does no I/O and never blocks on the network. Returns
    /// [`SafeProviders::Unknown`] when this model's endpoints have not been fetched yet
    /// in this process — call [`Self::ensure_safe_providers_loaded`] (or
    /// [`Self::refresh_provider_endpoints`] directly) first. Returns
    /// [`SafeProviders::Known`] once fetched, which may wrap an empty `Vec` when the
    /// model genuinely has zero clean providers right now.
    pub fn safe_providers_for(&self, model_id: &str) -> SafeProviders {
        match self.inner.lock() {
            Ok(inner) => match inner.provider_endpoints_cache.get(model_id) {
                Some(endpoints) => SafeProviders::Known(compute_safe_providers(endpoints)),
                None => SafeProviders::Unknown,
            },
            // A poisoned lock is exactly the "we don't know" case — fail closed, don't
            // pretend we have data.
            Err(_) => SafeProviders::Unknown,
        }
    }

    /// The full joined provider-endpoint list for `model_id` (pricing + data policy for
    /// every provider currently serving it), from cache only. `None` when not yet
    /// fetched. This is the richer sibling of [`Self::safe_providers_for`] — intended
    /// for the Pass-2 picker UI (full list with prices/badges), not for the Pass-1
    /// enforcement path, which only needs the safe-slug list.
    pub fn provider_endpoints_for(&self, model_id: &str) -> Option<Vec<ProviderEndpointInfo>> {
        self.inner
            .lock()
            .ok()
            .and_then(|inner| inner.provider_endpoints_cache.get(model_id).cloned())
    }

    /// Lazily ensure `model_id`'s provider-safety data is loaded (fetch on cache miss —
    /// including fetching the shared provider-policy catalog first if it too is
    /// uncached), then return the safe-provider set. Request-building call sites should
    /// use this: the FIRST request for a given model pays one (or two, the first time
    /// ever) network round trips; every subsequent request in this process reads cache.
    ///
    /// `api_key` is optional — both underlying endpoints are public — but is sent as an
    /// `Authorization` header when available, matching every other OpenRouter call
    /// site's convention. A fetch failure is NOT retried automatically here (mirrors
    /// [`Self::refresh_openrouter`]'s "cache the failure as empty" behavior for the
    /// provider-policy catalog); an empty per-model endpoint list is cached as
    /// `Known(vec![])` so a request-time caller fails closed via
    /// `provider_policy::provider_constraint_for_request` rather than re-fetching on
    /// every single request during an outage.
    pub async fn ensure_safe_providers_loaded(
        &self,
        api_key: Option<&str>,
        model_id: &str,
    ) -> SafeProviders {
        let already_cached = self
            .inner
            .lock()
            .map(|g| g.provider_endpoints_cache.contains_key(model_id))
            .unwrap_or(false);
        if !already_cached {
            self.refresh_provider_endpoints(api_key, model_id).await;
        }
        self.safe_providers_for(model_id)
    }

    /// Fetch `/api/v1/models/<model_id>/endpoints`, join with the (fetched-if-needed,
    /// cached) provider data-policy catalog, cache the joined result for `model_id`, and
    /// return it.
    ///
    /// On any error (network, parse) for the per-model endpoints fetch, logs to stderr
    /// and caches (and returns) an empty `Vec` — same "fail-safe-visible, not
    /// fail-silent-and-retry-forever" shape as [`Self::refresh_openrouter`]. A provider
    /// data-policy fetch failure does NOT abort this call: it is cached as an empty map
    /// (see [`Self::refresh_provider_policies`]), which — via
    /// [`join_endpoints_with_policy`]'s fail-closed unknown-provider path — makes every
    /// endpoint in this fetch resolve to unsafe, not silently safe.
    pub async fn refresh_provider_endpoints(
        &self,
        api_key: Option<&str>,
        model_id: &str,
    ) -> Vec<ProviderEndpointInfo> {
        let policy_needed = self
            .inner
            .lock()
            .map(|g| g.provider_policy_cache.is_none())
            .unwrap_or(true);
        if policy_needed {
            self.refresh_provider_policies(api_key).await;
        }

        let raw = match fetch_model_endpoints(api_key, model_id).await {
            Ok(eps) => eps,
            Err(err) => {
                eprintln!(
                    "[model-registry] OpenRouter /endpoints fetch failed for `{model_id}`: {err}"
                );
                Vec::new()
            }
        };

        let joined = {
            let policies = self
                .inner
                .lock()
                .ok()
                .and_then(|g| g.provider_policy_cache.clone())
                .unwrap_or_default();
            join_endpoints_with_policy(&raw, &policies)
        };

        if let Ok(mut inner) = self.inner.lock() {
            inner
                .provider_endpoints_cache
                .insert(model_id.to_string(), joined.clone());
        }
        joined
    }

    /// Fetch and cache the shared provider data-policy catalog
    /// (`/api/frontend/v1/all-providers`). On any error, logs to stderr and caches an
    /// empty map — every subsequent join treats every provider slug as unknown, which
    /// [`join_endpoints_with_policy`] resolves to UNSAFE, never safe.
    pub async fn refresh_provider_policies(
        &self,
        api_key: Option<&str>,
    ) -> HashMap<String, ProviderPolicyRecord> {
        let map = match fetch_provider_policies(api_key).await {
            Ok(m) => m,
            Err(err) => {
                eprintln!("[model-registry] OpenRouter /all-providers fetch failed: {err}");
                HashMap::new()
            }
        };
        if let Ok(mut inner) = self.inner.lock() {
            inner.provider_policy_cache = Some(map.clone());
        }
        map
    }

    /// Directly seed the per-model provider-endpoints cache — a TEST-ONLY seam, mirrors
    /// [`Self::seed_openrouter_entries`]. `#[doc(hidden)]`, not `#[cfg(test)]` (so
    /// integration tests in `tests/` can use it without a live HTTP call).
    #[doc(hidden)]
    pub fn seed_provider_endpoints(&self, model_id: &str, endpoints: Vec<ProviderEndpointInfo>) {
        if let Ok(mut inner) = self.inner.lock() {
            inner
                .provider_endpoints_cache
                .insert(model_id.to_string(), endpoints);
        }
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

// ── Per-provider data policy (the OpenRouter provider-safety trust core) ────────
//
// Two separate OpenRouter endpoints, joined here:
//
// 1. `GET /api/v1/models/<id>/endpoints` — PER-MODEL: which providers currently serve
//    this specific model, plus per-model pricing. Does NOT carry data-policy fields
//    (verified live 2026-08-06 against `deepseek/deepseek-chat` and
//    `qwen/qwen3-coder`). Each endpoint carries a `tag` (e.g. `"deepinfra/fp4"`,
//    `"streamlake"`) whose segment before the first `/` is the provider SLUG.
// 2. `GET /api/frontend/v1/all-providers` — PER-PROVIDER (not per-model): every
//    provider's `dataPolicy` block (`training`, `retainsPrompts`, `retentionDays`,
//    ...), keyed by `slug`. This is an undocumented-but-public frontend endpoint (no
//    `/docs/api-reference` page for it as of 2026-08-06); confirmed live and stable
//    enough to build on — see the design doc's "Pass 1 landed" section for the raw
//    verification transcript. Requires no API key (confirmed via an unauthenticated
//    curl); an `Authorization` header is still sent when a key is available, matching
//    every other OpenRouter call site's convention.
//
// The join key is `tag.split('/').next()` (the endpoint's provider slug) against the
// all-providers `slug` field — NOT a name match, which is unreliable (`"Google Vertex"`
// display name vs `"google-vertex"` slug vs the endpoint's `"Google"` provider_name).

/// One provider's data-policy `dataPolicy` block from `/api/frontend/v1/all-providers`.
///
/// Field names verified live 2026-08-06 against a real response (see design doc). All
/// fields are `#[serde(default)]` so a provider record with a missing/malformed
/// `dataPolicy` block still parses — it just resolves to `training: false,
/// retains_prompts: false` at the STRUCT level, but see [`join_endpoints_with_policy`]
/// for how a provider absent from the map entirely (not malformed — simply not found)
/// is treated as UNSAFE, not safe, which is the fail-closed direction that matters.
#[derive(Debug, Clone, Default, Deserialize)]
struct OpenRouterDataPolicy {
    #[serde(default)]
    training: bool,
    #[serde(default, rename = "retainsPrompts")]
    retains_prompts: bool,
    #[serde(default, rename = "retentionDays")]
    #[allow(dead_code)] // Carried for future Pass-2 UI display; not consumed by Pass 1 logic.
    retention_days: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterProviderRaw {
    slug: String,
    #[serde(default)]
    #[allow(dead_code)] // Display name; not needed for the safety join (slug is the key).
    name: String,
    #[serde(default)]
    headquarters: Option<String>,
    #[serde(default, rename = "dataPolicy")]
    data_policy: OpenRouterDataPolicy,
}

#[derive(Debug, Deserialize)]
struct OpenRouterAllProvidersResp {
    data: Vec<OpenRouterProviderRaw>,
}

/// A provider's data policy + region, keyed by slug in the cache — the join target for
/// [`join_endpoints_with_policy`]. `pub` because it appears in
/// [`ModelRegistry::refresh_provider_policies`]'s return type.
#[derive(Debug, Clone, Default)]
pub struct ProviderPolicyRecord {
    data_policy: OpenRouterDataPolicy,
    region: Option<String>,
}

/// One endpoint entry from `/api/v1/models/<id>/endpoints` (pre-join, no data policy).
#[derive(Debug, Deserialize)]
struct OpenRouterEndpointRaw {
    provider_name: String,
    /// The provider-variant slug, e.g. `"deepinfra/fp4"` or `"streamlake"`. `None`
    /// (missing/malformed) means the provider can't be resolved against the
    /// all-providers policy map — treated as unsafe, see [`join_endpoints_with_policy`].
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    pricing: OpenRouterPricing,
}

#[derive(Debug, Deserialize)]
struct OpenRouterEndpointsResp {
    data: OpenRouterEndpointsData,
}

#[derive(Debug, Deserialize)]
struct OpenRouterEndpointsData {
    #[serde(default)]
    endpoints: Vec<OpenRouterEndpointRaw>,
}

/// One provider's joined pricing + data-policy record for a SPECIFIC model —
/// `/endpoints` (which providers serve this model, at what price) joined with
/// `/all-providers` (does that provider train / retain).
///
/// This is the shape both [`ModelRegistry::safe_providers_for`] (Pass 1, the
/// enforcement seam) and the Pass-2 provider picker UI will read.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderEndpointInfo {
    /// The OpenRouter provider slug (e.g. `"deepinfra"`). This — NOT the display name —
    /// is what belongs in the request-body `provider.only` array.
    pub provider_slug: String,
    pub provider_name: String,
    /// Headquarters/region, when OpenRouter reports one (e.g. `"US"`, `"SG"`).
    pub region: Option<String>,
    /// `true` when this provider trains on prompts sent to it.
    pub training: bool,
    /// `true` when this provider retains (stores) prompts beyond serving the request.
    pub retains_prompts: bool,
    /// USD per million input/output tokens for this provider serving this model.
    pub price_in: f64,
    pub price_out: f64,
}

impl ProviderEndpointInfo {
    /// Camerata's safe-mode bar: no training AND no prompt retention. This is the ONLY
    /// place that definition lives — [`compute_safe_providers`] and
    /// `provider_policy::provider_constraint_for_request`'s callers both go through it.
    pub fn is_safe(&self) -> bool {
        !self.training && !self.retains_prompts
    }
}

/// Resolve the provider slug this endpoint's `tag` names — the segment before the first
/// `/` (e.g. `"deepinfra/fp4"` -> `"deepinfra"`, `"streamlake"` -> `"streamlake"`).
fn provider_slug_from_tag(tag: &str) -> &str {
    tag.split('/').next().unwrap_or(tag)
}

/// Join per-model endpoints with the provider data-policy map.
///
/// FAIL-CLOSED: an endpoint whose `tag` is missing, or whose resolved slug is not found
/// in `policies` (unknown provider, malformed data, policy catalog not yet loaded) is
/// recorded with `training: true, retains_prompts: true` — i.e. treated as UNSAFE. An
/// unknown data policy NEVER defaults to safe. This never panics on malformed input.
fn join_endpoints_with_policy(
    endpoints: &[OpenRouterEndpointRaw],
    policies: &HashMap<String, ProviderPolicyRecord>,
) -> Vec<ProviderEndpointInfo> {
    endpoints
        .iter()
        .map(|ep| {
            let slug = ep
                .tag
                .as_deref()
                .map(provider_slug_from_tag)
                .unwrap_or_default();
            let price_in = ep.pricing.prompt * 1_000_000.0;
            let price_out = ep.pricing.completion * 1_000_000.0;
            match policies.get(slug) {
                Some(record) => ProviderEndpointInfo {
                    provider_slug: slug.to_string(),
                    provider_name: ep.provider_name.clone(),
                    region: record.region.clone(),
                    training: record.data_policy.training,
                    retains_prompts: record.data_policy.retains_prompts,
                    price_in,
                    price_out,
                },
                None => ProviderEndpointInfo {
                    provider_slug: slug.to_string(),
                    provider_name: ep.provider_name.clone(),
                    region: None,
                    // Unknown policy — fail closed, never default to safe.
                    training: true,
                    retains_prompts: true,
                    price_in,
                    price_out,
                },
            }
        })
        .collect()
}

/// The safe-provider slugs (sorted, deduped) from a joined endpoint list: providers
/// satisfying [`ProviderEndpointInfo::is_safe`]. May be empty.
pub fn compute_safe_providers(endpoints: &[ProviderEndpointInfo]) -> Vec<String> {
    let mut slugs: Vec<String> = endpoints
        .iter()
        .filter(|e| e.is_safe())
        .map(|e| e.provider_slug.clone())
        .collect();
    slugs.sort();
    slugs.dedup();
    slugs
}

/// Fetch and parse `/api/frontend/v1/all-providers`, returning slug -> data policy.
/// `api_key` is optional (the endpoint is public) but sent when available, matching
/// every other OpenRouter call site's header convention. On any error (network, parse),
/// logs to stderr and returns an empty map — callers must treat an empty map the same
/// as "no data available" (fail closed via [`join_endpoints_with_policy`]'s
/// unknown-provider path), never as "every provider is safe."
async fn fetch_provider_policies(
    api_key: Option<&str>,
) -> anyhow::Result<HashMap<String, ProviderPolicyRecord>> {
    let client = reqwest::Client::new();
    let mut req = client
        .get("https://openrouter.ai/api/frontend/v1/all-providers")
        .header("HTTP-Referer", "https://camerata.ai")
        .header("X-Title", "Camerata");
    if let Some(key) = api_key {
        if !key.trim().is_empty() {
            req = req.header("Authorization", format!("Bearer {key}"));
        }
    }
    let resp = req.send().await?;
    if !resp.status().is_success() {
        anyhow::bail!(
            "OpenRouter /api/frontend/v1/all-providers returned {}",
            resp.status()
        );
    }
    let body: OpenRouterAllProvidersResp = resp.json().await?;
    let map = body
        .data
        .into_iter()
        .map(|p| {
            (
                p.slug,
                ProviderPolicyRecord {
                    data_policy: p.data_policy,
                    region: p.headquarters,
                },
            )
        })
        .collect();
    Ok(map)
}

/// Fetch and parse `/api/v1/models/<model_id>/endpoints`, returning the raw (pre-join)
/// endpoint list. `api_key` is optional (public endpoint) but sent when available.
async fn fetch_model_endpoints(
    api_key: Option<&str>,
    model_id: &str,
) -> anyhow::Result<Vec<OpenRouterEndpointRaw>> {
    let client = reqwest::Client::new();
    let mut req = client
        .get(format!("https://openrouter.ai/api/v1/models/{model_id}/endpoints"))
        .header("HTTP-Referer", "https://camerata.ai")
        .header("X-Title", "Camerata");
    if let Some(key) = api_key {
        if !key.trim().is_empty() {
            req = req.header("Authorization", format!("Bearer {key}"));
        }
    }
    let resp = req.send().await?;
    if !resp.status().is_success() {
        anyhow::bail!(
            "OpenRouter /api/v1/models/{model_id}/endpoints returned {}",
            resp.status()
        );
    }
    let body: OpenRouterEndpointsResp = resp.json().await?;
    Ok(body.data.endpoints)
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

    // ── Claude static list prices ─────────────────────────────────────────────
    //
    // These pins ensure the registry carries the correct Anthropic list prices so
    // that the onboarding cost estimator produces meaningful (non-zero) estimates.
    // If Anthropic changes list pricing, update CLAUDE_REGISTRY_MODELS AND these
    // tests together.

    #[test]
    fn claude_opus_carries_list_price_5_25() {
        let entries = claude_entries();
        let opus = entries.iter().find(|e| e.id == "claude-opus-4-8").unwrap();
        assert!(
            (opus.price_in - 5.0).abs() < f64::EPSILON,
            "Opus 4.8 price_in must be $5/M, got {}",
            opus.price_in
        );
        assert!(
            (opus.price_out - 25.0).abs() < f64::EPSILON,
            "Opus 4.8 price_out must be $25/M, got {}",
            opus.price_out
        );
    }

    #[test]
    fn claude_sonnet_carries_list_price_3_15() {
        let entries = claude_entries();
        let sonnet = entries.iter().find(|e| e.id == "claude-sonnet-4-6").unwrap();
        assert!(
            (sonnet.price_in - 3.0).abs() < f64::EPSILON,
            "Sonnet 4.6 price_in must be $3/M, got {}",
            sonnet.price_in
        );
        assert!(
            (sonnet.price_out - 15.0).abs() < f64::EPSILON,
            "Sonnet 4.6 price_out must be $15/M, got {}",
            sonnet.price_out
        );
    }

    #[test]
    fn claude_haiku_carries_list_price_1_5() {
        let entries = claude_entries();
        let haiku = entries
            .iter()
            .find(|e| e.id == "claude-haiku-4-5-20251001")
            .unwrap();
        assert!(
            (haiku.price_in - 1.0).abs() < f64::EPSILON,
            "Haiku 4.5 price_in must be $1/M, got {}",
            haiku.price_in
        );
        assert!(
            (haiku.price_out - 5.0).abs() < f64::EPSILON,
            "Haiku 4.5 price_out must be $5/M, got {}",
            haiku.price_out
        );
    }

    /// All Claude models must carry non-zero prices so the estimator produces meaningful
    /// (non-zero) dollar figures when those models are selected for AI scanning.
    #[test]
    fn all_claude_entries_have_nonzero_prices() {
        for e in claude_entries() {
            assert!(
                e.price_in > 0.0,
                "{} must have price_in > 0 (estimator needs real prices)",
                e.id
            );
            assert!(
                e.price_out > 0.0,
                "{} must have price_out > 0 (estimator needs real prices)",
                e.id
            );
        }
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
            architecture: OpenRouterArchitecture::default(),
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
            architecture: OpenRouterArchitecture::default(),
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

    /// `try_refresh_from_store` is the single code path all THREE OpenRouter refresh
    /// triggers (server startup, post-credential-save, and the manual UI button) funnel
    /// through. This locks its no-key contract directly and synchronously — no live HTTP
    /// call, no panic, cache stays un-fetched (not "fetched-empty") — so the startup and
    /// credential-save call sites don't each need their own network-adjacent test to prove
    /// the same graceful behaviour.
    #[tokio::test]
    async fn try_refresh_from_store_is_a_graceful_noop_with_no_key_configured() {
        let reg = ModelRegistry::new();
        let creds = crate::credentials::MemoryCredentialStore::new();
        // No key ever set on this store — mirrors a fresh install / no OpenRouter key saved.
        let attempted = reg.try_refresh_from_store(&creds).await;
        assert!(!attempted, "no key configured: must no-op, not attempt a live call");
        assert!(
            !reg.openrouter_fetched(),
            "cache must stay unpopulated (None), not Some([]) — that's the fetch-attempted state"
        );
        let all = reg.all_entries();
        assert!(all.iter().any(|e| e.provider == "claude"), "Claude entries unaffected");
        assert!(!all.iter().any(|e| e.provider == "openrouter"), "no OpenRouter entries appear");
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
            caching: false,
            vision: false,
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

    // `registry_entry_serde_roundtrip` moved to
    // `camerata_api_types::model_registry::tests` along with `RegistryEntry`.

    // ── caching_heuristic ─────────────────────────────────────────────────────

    #[test]
    fn caching_heuristic_claude_provider_always_true() {
        // All claude-provider models are caching-capable (the subscription/CLI path).
        assert!(caching_heuristic("claude", "claude-opus-4-8"));
        assert!(caching_heuristic("claude", "claude-sonnet-4-6"));
        assert!(caching_heuristic("claude", "claude-haiku-4-5-20251001"));
    }

    #[test]
    fn caching_heuristic_deepseek_openrouter_true() {
        assert!(caching_heuristic("openrouter", "deepseek/deepseek-r1"));
        assert!(caching_heuristic("openrouter", "deepseek/deepseek-chat"));
    }

    #[test]
    fn caching_heuristic_gemini_openrouter_true() {
        assert!(caching_heuristic("openrouter", "google/gemini-2.0-flash-001"));
        assert!(caching_heuristic("openrouter", "google/gemini-pro"));
    }

    #[test]
    fn caching_heuristic_anthropic_openrouter_true() {
        assert!(caching_heuristic("openrouter", "anthropic/claude-3-5-sonnet"));
    }

    #[test]
    fn caching_heuristic_random_openrouter_model_false() {
        assert!(!caching_heuristic("openrouter", "meta-llama/llama-3.1-8b-instruct"));
        assert!(!caching_heuristic("openrouter", "openai/gpt-4o"));
        assert!(!caching_heuristic("openrouter", "qwen/qwen3-235b-a22b"));
    }

    #[test]
    fn claude_entries_all_have_caching_true() {
        for e in claude_entries() {
            assert!(e.caching, "{} must have caching=true", e.id);
        }
    }

    // ── vision flag ───────────────────────────────────────────────────────────

    #[test]
    fn claude_4x_entries_all_have_vision_true() {
        // All three Claude 4.x models in the static catalog are multimodal.
        for e in claude_entries() {
            assert!(
                e.vision,
                "{} must have vision=true (all Claude 4.x models are multimodal)",
                e.id
            );
        }
    }

    #[test]
    fn openrouter_model_with_image_modality_has_vision_true() {
        // An OpenRouter model whose architecture.input_modalities includes "image"
        // must have vision=true after conversion.
        let raw = OpenRouterModelRaw {
            id: "minimax/minimax-01".to_string(),
            name: "MiniMax-01".to_string(),
            pricing: OpenRouterPricing { prompt: 0.0, completion: 0.0 },
            context_length: 1_000_000,
            supported_parameters: vec!["tools".to_string()],
            architecture: OpenRouterArchitecture {
                input_modalities: vec!["text".to_string(), "image".to_string()],
            },
        };
        let entry = raw.to_entry();
        assert!(
            entry.vision,
            "OpenRouter model with input_modalities=[text,image] must have vision=true"
        );
    }

    #[test]
    fn openrouter_model_without_image_modality_has_vision_false() {
        // A text-only OpenRouter model must have vision=false.
        let raw = OpenRouterModelRaw {
            id: "qwen/qwen3-235b:free".to_string(),
            name: "Qwen3 235B (free)".to_string(),
            pricing: OpenRouterPricing { prompt: 0.0, completion: 0.0 },
            context_length: 32_768,
            supported_parameters: vec!["tools".to_string()],
            architecture: OpenRouterArchitecture {
                input_modalities: vec!["text".to_string()],
            },
        };
        let entry = raw.to_entry();
        assert!(
            !entry.vision,
            "OpenRouter model with only text modality must have vision=false"
        );
    }

    #[test]
    fn openrouter_model_with_empty_modalities_has_vision_false() {
        // A model that omits the architecture block entirely (defaults to empty) must
        // have vision=false — no false positives from the default.
        let raw = OpenRouterModelRaw {
            id: "some/text-model".to_string(),
            name: "Text Model".to_string(),
            pricing: OpenRouterPricing { prompt: 0.0, completion: 0.0 },
            context_length: 8_192,
            supported_parameters: vec![],
            architecture: OpenRouterArchitecture::default(),
        };
        let entry = raw.to_entry();
        assert!(
            !entry.vision,
            "OpenRouter model with no modalities must have vision=false"
        );
    }

    // ── Provider-safety data: parsing, join, safe_providers_for ─────────────────

    /// A real (trimmed) `/api/v1/models/<id>/endpoints` response body, captured live
    /// 2026-08-06 against `deepseek/deepseek-chat` (see the design doc's "Pass 1
    /// landed" section for the full transcript). Exercises the real field names/shapes:
    /// `data.endpoints[].{provider_name,tag,pricing:{prompt,completion}}`.
    const SAMPLE_ENDPOINTS_JSON: &str = r#"{
        "data": {
            "id": "deepseek/deepseek-chat",
            "name": "DeepSeek: DeepSeek V3",
            "endpoints": [
                {
                    "name": "StreamLake | deepseek/deepseek-chat-v3",
                    "provider_name": "StreamLake",
                    "tag": "streamlake",
                    "pricing": {"prompt": "0.0000002574", "completion": "0.0000010287", "discount": 0.1}
                },
                {
                    "name": "DeepInfra | deepseek/deepseek-chat-v3",
                    "provider_name": "DeepInfra",
                    "tag": "deepinfra/fp4",
                    "pricing": {"prompt": "0.00000032", "completion": "0.00000089", "discount": 0}
                },
                {
                    "name": "Novita | deepseek/deepseek-chat-v3",
                    "provider_name": "Novita",
                    "tag": "novita/fp8",
                    "pricing": {"prompt": "0.0000004", "completion": "0.0000013", "discount": 0}
                }
            ]
        }
    }"#;

    /// A real (trimmed) `/api/frontend/v1/all-providers` response body, captured live
    /// 2026-08-06. Exercises the real field names: `data[].{slug,headquarters,
    /// dataPolicy:{training,retainsPrompts,retentionDays}}`. Includes DeepSeek's
    /// first-party listing (`training: true` — the training provider a safe list must
    /// exclude) and StreamLake (`retainsPrompts: true`, no `retentionDays` — the
    /// retaining-but-not-training provider a safe list must also exclude).
    const SAMPLE_ALL_PROVIDERS_JSON: &str = r#"{
        "data": [
            {
                "name": "DeepInfra",
                "slug": "deepinfra",
                "dataPolicy": {"training": false, "retainsPrompts": false},
                "headquarters": "US"
            },
            {
                "name": "Novita",
                "slug": "novita",
                "dataPolicy": {"training": false, "retainsPrompts": false},
                "headquarters": "US"
            },
            {
                "name": "StreamLake",
                "slug": "streamlake",
                "dataPolicy": {"training": false, "retainsPrompts": true}
            },
            {
                "name": "DeepSeek",
                "slug": "deepseek",
                "dataPolicy": {"training": true, "retainsPrompts": true}
            },
            {
                "name": "Cohere",
                "slug": "cohere",
                "dataPolicy": {"training": false, "retainsPrompts": true, "retentionDays": 30}
            }
        ]
    }"#;

    fn sample_policies() -> HashMap<String, ProviderPolicyRecord> {
        let resp: OpenRouterAllProvidersResp =
            serde_json::from_str(SAMPLE_ALL_PROVIDERS_JSON).unwrap();
        resp.data
            .into_iter()
            .map(|p| {
                (
                    p.slug,
                    ProviderPolicyRecord {
                        data_policy: p.data_policy,
                        region: p.headquarters,
                    },
                )
            })
            .collect()
    }

    fn sample_endpoints() -> Vec<OpenRouterEndpointRaw> {
        let resp: OpenRouterEndpointsResp = serde_json::from_str(SAMPLE_ENDPOINTS_JSON).unwrap();
        resp.data.endpoints
    }

    #[test]
    fn real_endpoints_response_parses_without_panicking() {
        let endpoints = sample_endpoints();
        assert_eq!(endpoints.len(), 3);
        assert_eq!(endpoints[0].provider_name, "StreamLake");
        assert_eq!(endpoints[0].tag.as_deref(), Some("streamlake"));
        assert_eq!(endpoints[1].tag.as_deref(), Some("deepinfra/fp4"));
    }

    #[test]
    fn real_all_providers_response_parses_without_panicking() {
        let policies = sample_policies();
        assert_eq!(policies.len(), 5);
        let deepinfra = policies.get("deepinfra").unwrap();
        assert!(!deepinfra.data_policy.training);
        assert!(!deepinfra.data_policy.retains_prompts);
        assert_eq!(deepinfra.region.as_deref(), Some("US"));
    }

    #[test]
    fn provider_slug_from_tag_splits_on_first_slash() {
        assert_eq!(provider_slug_from_tag("deepinfra/fp4"), "deepinfra");
        assert_eq!(provider_slug_from_tag("streamlake"), "streamlake");
        assert_eq!(provider_slug_from_tag("google-vertex/us-south1"), "google-vertex");
    }

    #[test]
    fn join_produces_exactly_the_clean_providers_for_safe_set() {
        let endpoints = sample_endpoints();
        let policies = sample_policies();
        let joined = join_endpoints_with_policy(&endpoints, &policies);
        assert_eq!(joined.len(), 3);

        let safe = compute_safe_providers(&joined);
        assert_eq!(safe, vec!["deepinfra".to_string(), "novita".to_string()]);
    }

    /// A China-hosted / training provider (DeepSeek's own first-party endpoint) must be
    /// excluded from the safe set. Simulated by adding a fourth endpoint whose tag
    /// resolves to the `deepseek` slug (training=true in the sample policy catalog).
    #[test]
    fn training_provider_excluded_from_safe_set() {
        let mut endpoints = sample_endpoints();
        endpoints.push(OpenRouterEndpointRaw {
            provider_name: "DeepSeek".to_string(),
            tag: Some("deepseek".to_string()),
            pricing: OpenRouterPricing { prompt: 0.0000002, completion: 0.0000008 },
        });
        let policies = sample_policies();
        let joined = join_endpoints_with_policy(&endpoints, &policies);
        let safe = compute_safe_providers(&joined);
        assert!(
            !safe.contains(&"deepseek".to_string()),
            "training provider must never appear in the safe set: {safe:?}"
        );
        // The other two clean providers are still present.
        assert_eq!(safe, vec!["deepinfra".to_string(), "novita".to_string()]);
    }

    /// A retaining-but-not-training provider (StreamLake) is ALSO excluded — the safe
    /// bar is `training == false && retains_prompts == false`, not `training == false`
    /// alone.
    #[test]
    fn retaining_only_provider_excluded_from_safe_set() {
        let endpoints = sample_endpoints();
        let policies = sample_policies();
        let joined = join_endpoints_with_policy(&endpoints, &policies);
        let streamlake = joined.iter().find(|e| e.provider_slug == "streamlake").unwrap();
        assert!(streamlake.retains_prompts);
        assert!(!streamlake.training);
        assert!(!streamlake.is_safe());
        let safe = compute_safe_providers(&joined);
        assert!(!safe.contains(&"streamlake".to_string()));
    }

    /// Malformed/missing fields (no `tag` at all, or a `tag` whose slug isn't in the
    /// policy catalog) must not panic, and must resolve to UNSAFE — never silently safe.
    #[test]
    fn missing_tag_and_unknown_slug_are_excluded_not_panicking() {
        let endpoints = vec![
            OpenRouterEndpointRaw {
                provider_name: "No Tag Provider".to_string(),
                tag: None,
                pricing: OpenRouterPricing::default(),
            },
            OpenRouterEndpointRaw {
                provider_name: "Unknown Provider".to_string(),
                tag: Some("totally-unknown-slug/variant".to_string()),
                pricing: OpenRouterPricing::default(),
            },
        ];
        let policies = sample_policies();
        // Must not panic.
        let joined = join_endpoints_with_policy(&endpoints, &policies);
        assert_eq!(joined.len(), 2);
        for entry in &joined {
            assert!(
                !entry.is_safe(),
                "unknown/malformed provider `{}` must resolve to unsafe (fail-closed)",
                entry.provider_slug
            );
            assert!(entry.training && entry.retains_prompts);
        }
        let safe = compute_safe_providers(&joined);
        assert!(safe.is_empty());
    }

    #[test]
    fn empty_policy_catalog_makes_every_endpoint_unsafe() {
        // Simulates the all-providers fetch having failed (cached as an empty map).
        let endpoints = sample_endpoints();
        let joined = join_endpoints_with_policy(&endpoints, &HashMap::new());
        assert_eq!(joined.len(), 3);
        assert!(joined.iter().all(|e| !e.is_safe()));
        assert!(compute_safe_providers(&joined).is_empty());
    }

    #[test]
    fn compute_safe_providers_dedupes_and_sorts() {
        let entries = vec![
            ProviderEndpointInfo {
                provider_slug: "novita".to_string(),
                provider_name: "Novita".to_string(),
                region: None,
                training: false,
                retains_prompts: false,
                price_in: 1.0,
                price_out: 1.0,
            },
            ProviderEndpointInfo {
                provider_slug: "deepinfra".to_string(),
                provider_name: "DeepInfra".to_string(),
                region: None,
                training: false,
                retains_prompts: false,
                price_in: 1.0,
                price_out: 1.0,
            },
            // Duplicate slug (same provider serving via two variants/quantizations).
            ProviderEndpointInfo {
                provider_slug: "deepinfra".to_string(),
                provider_name: "DeepInfra (fp8)".to_string(),
                region: None,
                training: false,
                retains_prompts: false,
                price_in: 2.0,
                price_out: 2.0,
            },
        ];
        let safe = compute_safe_providers(&entries);
        assert_eq!(safe, vec!["deepinfra".to_string(), "novita".to_string()]);
    }

    // ── ModelRegistry.safe_providers_for / seed_provider_endpoints ─────────────

    #[test]
    fn safe_providers_for_unknown_before_any_fetch() {
        let reg = ModelRegistry::new();
        assert_eq!(reg.safe_providers_for("some/model"), SafeProviders::Unknown);
    }

    #[test]
    fn safe_providers_for_known_after_seeding() {
        let reg = ModelRegistry::new();
        let endpoints = join_endpoints_with_policy(&sample_endpoints(), &sample_policies());
        reg.seed_provider_endpoints("deepseek/deepseek-chat", endpoints);
        match reg.safe_providers_for("deepseek/deepseek-chat") {
            SafeProviders::Known(list) => {
                assert_eq!(list, vec!["deepinfra".to_string(), "novita".to_string()]);
            }
            SafeProviders::Unknown => panic!("expected Known after seeding"),
        }
        // A DIFFERENT model id, never seeded, is still Unknown.
        assert_eq!(
            reg.safe_providers_for("some/other-model"),
            SafeProviders::Unknown
        );
    }

    #[test]
    fn safe_providers_for_known_empty_when_seeded_with_zero_safe_providers() {
        let reg = ModelRegistry::new();
        // Seed with only unsafe providers.
        let joined = join_endpoints_with_policy(
            &[OpenRouterEndpointRaw {
                provider_name: "StreamLake".to_string(),
                tag: Some("streamlake".to_string()),
                pricing: OpenRouterPricing::default(),
            }],
            &sample_policies(),
        );
        reg.seed_provider_endpoints("all-unsafe/model", joined);
        assert_eq!(
            reg.safe_providers_for("all-unsafe/model"),
            SafeProviders::Known(vec![])
        );
    }

    #[tokio::test]
    async fn ensure_safe_providers_loaded_is_a_noop_read_when_already_cached() {
        // No network in this test: seed the cache directly, then call the lazy-load
        // wrapper — it must return the seeded value without attempting any I/O (which
        // would fail/hang in a sandboxed test run if it tried).
        let reg = ModelRegistry::new();
        let endpoints = join_endpoints_with_policy(&sample_endpoints(), &sample_policies());
        reg.seed_provider_endpoints("cached/model", endpoints);
        let result = reg.ensure_safe_providers_loaded(None, "cached/model").await;
        assert_eq!(
            result,
            SafeProviders::Known(vec!["deepinfra".to_string(), "novita".to_string()])
        );
    }
}
