//! Model Efficiency Profile cascade: compute concrete model assignments for ALL entry
//! points from a profile + the model registry.
//!
//! `compute_profile_cascade` is a PURE function (no I/O) — it reads the registry and
//! returns the proposed assignments. The caller decides whether to apply them.

use crate::model_registry::ModelRegistry;
use crate::model_tier::TierMap;
use crate::project::{L3ReviewConfig, ModelProfile, StepModels};

// ── Known model ids ───────────────────────────────────────────────────────────

const OPUS: &str = "claude-opus-4-8";
const SONNET: &str = "claude-sonnet-4-6";
const HAIKU: &str = "claude-haiku-4-5-20251001";

// ── Assignment output ─────────────────────────────────────────────────────────

/// The concrete model assignments produced by a profile cascade.
///
/// `tier_map`, `step_models`, and `l3_review` are the same types stored on `Project`,
/// so applying the cascade is a direct field-by-field copy.
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileAssignments {
    pub tier_map: TierMap,
    pub step_models: StepModels,
    pub l3_review: L3ReviewConfig,
}

// ── Cascade computation ───────────────────────────────────────────────────────

/// Compute the concrete assignments for every model entry point from `profile` + `registry`.
///
/// Returns `None` only for `ModelProfile::Custom` (no-op; caller leaves everything alone).
///
/// For `MaxEfficiency`, picks free + tool-use models from the registry ranked by coding
/// score. If no free models are available, gracefully falls back to Balanced (paid) values
/// for those slots so the profile always produces a usable result.
pub fn compute_profile_cascade(
    profile: ModelProfile,
    registry: &ModelRegistry,
) -> Option<ProfileAssignments> {
    match profile {
        ModelProfile::Custom => None,
        ModelProfile::Balanced => Some(balanced_assignments()),
        ModelProfile::MaxQuality => Some(max_quality_assignments()),
        ModelProfile::MaxEfficiency => Some(max_efficiency_assignments(registry)),
    }
}

// ── Balanced ──────────────────────────────────────────────────────────────────

fn balanced_assignments() -> ProfileAssignments {
    ProfileAssignments {
        tier_map: TierMap {
            strongest: OPUS.to_string(),
            balanced: vec![SONNET.to_string()],
            fast: vec![HAIKU.to_string()],
            vision: vec![],
        },
        step_models: StepModels {
            audit: HAIKU.to_string(),
            calibration: HAIKU.to_string(),
            research_chat: HAIKU.to_string(),
            story_authoring: HAIKU.to_string(),
            decomposition: HAIKU.to_string(),
            escalation: HAIKU.to_string(),
            clarification: HAIKU.to_string(),
        },
        l3_review: L3ReviewConfig {
            enabled: false,
            model: String::new(),
        },
    }
}

// ── MaxQuality ────────────────────────────────────────────────────────────────

fn max_quality_assignments() -> ProfileAssignments {
    ProfileAssignments {
        tier_map: TierMap {
            strongest: OPUS.to_string(),
            balanced: vec![SONNET.to_string()],
            fast: vec![SONNET.to_string()],
            vision: vec![],
        },
        step_models: StepModels {
            audit: SONNET.to_string(),
            calibration: SONNET.to_string(),
            research_chat: SONNET.to_string(),
            story_authoring: SONNET.to_string(),
            decomposition: SONNET.to_string(),
            escalation: SONNET.to_string(),
            clarification: SONNET.to_string(),
        },
        l3_review: L3ReviewConfig {
            enabled: true,
            model: SONNET.to_string(),
        },
    }
}

// ── MaxEfficiency ─────────────────────────────────────────────────────────────

/// Pick the best free + tool-use model from the registry by coding score (descending).
/// Returns `None` when the registry has no free tool-use models.
fn best_free_coder(registry: &ModelRegistry) -> Option<String> {
    registry
        .all_entries()
        .into_iter()
        .filter(|e| e.free && e.tool_use && e.provider != "claude")
        .max_by(|a, b| a.coding.partial_cmp(&b.coding).unwrap_or(std::cmp::Ordering::Equal))
        .map(|e| e.id)
}

/// Pick the smallest (lowest coding score, still free + tool-use) free model for the fast
/// chain. Falls back to `best_free_coder` when there is only one free model.
fn smallest_free_model(registry: &ModelRegistry) -> Option<String> {
    let mut free: Vec<_> = registry
        .all_entries()
        .into_iter()
        .filter(|e| e.free && e.tool_use && e.provider != "claude")
        .collect();
    if free.is_empty() {
        return None;
    }
    free.sort_by(|a, b| a.coding.partial_cmp(&b.coding).unwrap_or(std::cmp::Ordering::Equal));
    Some(free[0].id.clone())
}

fn max_efficiency_assignments(registry: &ModelRegistry) -> ProfileAssignments {
    let top_free = best_free_coder(registry);
    let small_free = smallest_free_model(registry);

    // Graceful fallback: if no free models available, use Balanced paid values for those slots.
    let balanced_chain = match &top_free {
        Some(free_id) => vec![free_id.clone(), SONNET.to_string()],
        None => vec![SONNET.to_string()], // fallback to paid
    };

    let fast_chain = match &small_free {
        Some(free_id) => vec![free_id.clone(), HAIKU.to_string()],
        None => vec![HAIKU.to_string()], // fallback to paid
    };

    let step_model = match &top_free {
        Some(free_id) => free_id.clone(),
        None => HAIKU.to_string(), // fallback to cheapest paid
    };

    let l3_model = match &top_free {
        Some(free_id) => free_id.clone(),
        None => String::new(), // fallback: L3 disabled
    };
    let l3_enabled = top_free.is_some();

    ProfileAssignments {
        tier_map: TierMap {
            strongest: OPUS.to_string(),
            balanced: balanced_chain,
            fast: fast_chain,
            vision: vec![],
        },
        step_models: StepModels {
            audit: step_model.clone(),
            calibration: step_model.clone(),
            research_chat: step_model.clone(),
            story_authoring: step_model.clone(),
            decomposition: step_model.clone(),
            escalation: step_model.clone(),
            clarification: step_model,
        },
        l3_review: L3ReviewConfig {
            enabled: l3_enabled,
            model: l3_model,
        },
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_registry::{ModelRegistry, RegistryEntry};

    fn make_free_coder(id: &str) -> RegistryEntry {
        RegistryEntry {
            provider: "openrouter".to_string(),
            display: id.to_string(),
            id: id.to_string(),
            free: true,
            tool_use: true,
            context: 32_768,
            coding: 1.0,
            price_in: 0.0,
            price_out: 0.0,
            weight: 0,
            caching: false,
            vision: false,
        }
    }

    fn make_free_small(id: &str) -> RegistryEntry {
        RegistryEntry {
            provider: "openrouter".to_string(),
            display: id.to_string(),
            id: id.to_string(),
            free: true,
            tool_use: true,
            context: 8_192,
            coding: 0.7,
            price_in: 0.0,
            price_out: 0.0,
            weight: 0,
            caching: false,
            vision: false,
        }
    }

    // ── Custom is a no-op ─────────────────────────────────────────────────────

    #[test]
    fn custom_profile_returns_none() {
        let reg = ModelRegistry::new();
        assert!(compute_profile_cascade(ModelProfile::Custom, &reg).is_none());
    }

    // ── Balanced ─────────────────────────────────────────────────────────────

    #[test]
    fn balanced_profile_uses_paid_claude_tiers() {
        let reg = ModelRegistry::new();
        let a = compute_profile_cascade(ModelProfile::Balanced, &reg).unwrap();
        assert_eq!(a.tier_map.strongest, OPUS);
        assert_eq!(a.tier_map.balanced, vec![SONNET.to_string()]);
        assert_eq!(a.tier_map.fast, vec![HAIKU.to_string()]);
        // All step models are Haiku.
        assert_eq!(a.step_models.audit, HAIKU);
        assert_eq!(a.step_models.clarification, HAIKU);
        // L3 off.
        assert!(!a.l3_review.enabled);
    }

    #[test]
    fn balanced_default_profile_is_balanced() {
        assert_eq!(ModelProfile::default(), ModelProfile::Balanced);
    }

    // ── MaxQuality ────────────────────────────────────────────────────────────

    #[test]
    fn max_quality_uses_opus_sonnet_throughout() {
        let reg = ModelRegistry::new();
        let a = compute_profile_cascade(ModelProfile::MaxQuality, &reg).unwrap();
        assert_eq!(a.tier_map.strongest, OPUS);
        assert_eq!(a.tier_map.balanced, vec![SONNET.to_string()]);
        assert_eq!(a.tier_map.fast, vec![SONNET.to_string()]);
        assert_eq!(a.step_models.audit, SONNET);
        // L3 on with Sonnet.
        assert!(a.l3_review.enabled);
        assert_eq!(a.l3_review.model, SONNET);
    }

    // ── MaxEfficiency — free models present ───────────────────────────────────

    #[test]
    fn max_efficiency_picks_free_models_when_available() {
        let reg = ModelRegistry::new();
        reg.seed_openrouter_entries(vec![
            make_free_coder("qwen/qwen3-coder:free"),
            make_free_small("meta/llama-small:free"),
        ]);

        let a = compute_profile_cascade(ModelProfile::MaxEfficiency, &reg).unwrap();

        // Strongest stays Opus.
        assert_eq!(a.tier_map.strongest, OPUS);
        // Balanced chain: best free coder + Sonnet fallback.
        assert_eq!(a.tier_map.balanced[0], "qwen/qwen3-coder:free");
        assert_eq!(a.tier_map.balanced[1], SONNET);
        // Fast chain: smallest free + Haiku fallback.
        assert!(a.tier_map.fast.len() == 2);
        assert_eq!(a.tier_map.fast[1], HAIKU);
        // Step models use free.
        assert!(!a.step_models.audit.starts_with("claude"));
        // L3 enabled with free model.
        assert!(a.l3_review.enabled);
    }

    // ── MaxEfficiency — no free models (graceful paid fallback) ───────────────

    #[test]
    fn max_efficiency_falls_back_to_paid_when_no_free_models() {
        // Registry has no OpenRouter entries at all (the cache is empty = no free models).
        let reg = ModelRegistry::new();
        // Do NOT seed any openrouter entries; cache stays None (= not fetched).
        // The cascade must still produce a valid (paid) result.
        let a = compute_profile_cascade(ModelProfile::MaxEfficiency, &reg).unwrap();

        // Falls back to paid Balanced values for chains.
        assert_eq!(a.tier_map.strongest, OPUS);
        assert_eq!(a.tier_map.balanced, vec![SONNET.to_string()]);
        assert_eq!(a.tier_map.fast, vec![HAIKU.to_string()]);
        // Step model falls back to Haiku (cheapest paid).
        assert_eq!(a.step_models.audit, HAIKU);
        // L3 disabled (no free model to run it on).
        assert!(!a.l3_review.enabled);
    }

    // ── MaxEfficiency — empty seeded cache (also no free models) ─────────────

    #[test]
    fn max_efficiency_falls_back_to_paid_when_cache_is_empty_vec() {
        let reg = ModelRegistry::new();
        reg.seed_openrouter_entries(vec![]); // cache set to Some([]) — fetched but empty
        let a = compute_profile_cascade(ModelProfile::MaxEfficiency, &reg).unwrap();
        assert_eq!(a.tier_map.balanced, vec![SONNET.to_string()]);
        assert_eq!(a.tier_map.fast, vec![HAIKU.to_string()]);
        assert!(!a.l3_review.enabled);
    }

    // ── preview != apply ──────────────────────────────────────────────────────

    #[test]
    fn compute_cascade_does_not_mutate_registry() {
        // compute_profile_cascade is pure; calling it multiple times with the same
        // registry always returns the same result (idempotent).
        let reg = ModelRegistry::new();
        reg.seed_openrouter_entries(vec![make_free_coder("q/q:free")]);
        let a1 = compute_profile_cascade(ModelProfile::MaxEfficiency, &reg);
        let a2 = compute_profile_cascade(ModelProfile::MaxEfficiency, &reg);
        assert_eq!(a1, a2);
    }
}
